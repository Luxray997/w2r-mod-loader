//! A signed report: `{"v":1,"key":"<64 hex>","sig":"<128 hex>","body":{report}}`. The signature
//! covers `w2r-report-1\n` followed by the compact JSON of `body` (`Value::to_json`, which keeps
//! key order and number text, so a parsed body serialises back to the same bytes).

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use w2r_formats::json::{self, Number, Value};

use crate::hex;
use crate::identity::Identity;
use crate::report::Report;

const DOMAIN: &[u8] = b"w2r-report-1\n";
pub const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub key: [u8; 32],
    pub sig: [u8; 64],
    pub body: Value,
}

fn message(body: &Value) -> Vec<u8> {
    let mut m = DOMAIN.to_vec();
    m.extend_from_slice(body.to_json().as_bytes());
    m
}

/// Signs `report` with the installation's identity.
pub fn seal(identity: &Identity, report: &Report) -> Envelope {
    let body = report.to_value();
    let sig = identity.sign(&message(&body));
    Envelope {
        key: identity.public(),
        sig,
        body,
    }
}

impl Envelope {
    pub fn to_json(&self) -> String {
        Value::Object(vec![
            ("v".into(), Value::Number(Number { text: "1".into() })),
            ("key".into(), Value::String(hex::encode(&self.key))),
            ("sig".into(), Value::String(hex::encode(&self.sig))),
            ("body".into(), self.body.clone()),
        ])
        .to_json()
    }

    pub fn parse(raw: &[u8]) -> Result<Envelope, &'static str> {
        if raw.len() > MAX_BYTES {
            return Err("too large");
        }
        let v = json::parse(raw).map_err(|_| "not json")?;
        if v.get("v")
            .and_then(Value::as_number)
            .and_then(Number::as_u64)
            != Some(1)
        {
            return Err("unsupported version");
        }
        let key = v
            .get("key")
            .and_then(Value::as_str)
            .and_then(hex::decode_array::<32>)
            .ok_or("bad key")?;
        let sig = v
            .get("sig")
            .and_then(Value::as_str)
            .and_then(hex::decode_array::<64>)
            .ok_or("bad sig")?;
        let body = v.get("body").ok_or("missing body")?.clone();
        Ok(Envelope { key, sig, body })
    }

    /// True when `sig` is a valid signature by `key` over the body.
    pub fn verify(&self) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(&self.key) else {
            return false;
        };
        key.verify(&message(&self.body), &Signature::from_bytes(&self.sig))
            .is_ok()
    }

    /// The report, after the signature checks out.
    pub fn report(&self) -> Result<Report, &'static str> {
        if !self.verify() {
            return Err("bad signature");
        }
        Report::from_value(&self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Phase, setup_id};
    use crate::slot::{Kind, Player};

    fn report() -> Report {
        let players = vec![Player {
            slot: 1,
            kind: Kind::Human,
            name: "MrKrinkle".into(),
            tag: Some("MrKrinkle#11951".into()),
            team: 1,
            race: 0,
        }];
        Report {
            phase: Phase::Start,
            rid: [9; 8],
            setup: setup_id("m", &players),
            started: 1_760_000_000,
            client: "w2rpm 0.1.0".into(),
            fingerprint: "00000000".into(),
            map: "m".into(),
            me: 1,
            players,
            game: None,
            result: None,
        }
    }

    #[test]
    fn seal_text_parse_verify_round_trip() {
        let id = Identity::from_secret([3; 32]);
        let sealed = seal(&id, &report());
        assert!(sealed.verify());
        let text = sealed.to_json();
        let back = Envelope::parse(text.as_bytes()).unwrap();
        assert_eq!(back, sealed);
        assert_eq!(back.report().unwrap(), report());
        assert_eq!(back.key, id.public());
    }

    #[test]
    fn any_change_to_the_body_breaks_the_signature() {
        let id = Identity::from_secret([3; 32]);
        let text = seal(&id, &report()).to_json();
        let forged = text.replace("MrKrinkle#11951", "SomeoneElse#1");
        assert_ne!(forged, text);
        let env = Envelope::parse(forged.as_bytes()).unwrap();
        assert!(!env.verify());
        assert_eq!(env.report(), Err("bad signature"));
    }

    #[test]
    fn a_different_key_does_not_verify() {
        let a = Identity::from_secret([3; 32]);
        let b = Identity::from_secret([4; 32]);
        let mut env = seal(&a, &report());
        env.key = b.public();
        assert!(!env.verify());
    }

    #[test]
    fn rejects_malformed_envelopes() {
        assert!(Envelope::parse(b"nope").is_err());
        assert!(Envelope::parse(b"{\"v\":2}").is_err());
        assert!(Envelope::parse(b"{\"v\":1,\"key\":\"00\",\"sig\":\"00\",\"body\":{}}").is_err());
        assert!(Envelope::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
    }
}
