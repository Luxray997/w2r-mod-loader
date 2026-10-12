//! Sharing match data (docs/stats-service.md). The logic lives in `w2r_stats::share`, shared with
//! the in-game host; the manager adds the HTTPS transport (the host uses WinHTTP).

pub use w2r_stats::share::*;

use crate::fetch::Https;

const MAX_RESPONSE: u64 = 64 * 1024;

impl Poster for Https {
    fn post_json(&self, url: &str, body: &[u8]) -> Result<Vec<u8>, Error> {
        let mut resp = self
            .agent()
            .post(url)
            .header("content-type", "application/json")
            .send(body)
            .map_err(|e| Error::new("network", format!("{url}: {e}")))?;
        resp.body_mut()
            .with_config()
            .limit(MAX_RESPONSE)
            .read_to_vec()
            .map_err(|e| Error::new("network", format!("{url}: {e}")))
    }
}

impl From<Error> for crate::Error {
    fn from(e: Error) -> crate::Error {
        crate::Error::new(e.code, e.detail)
    }
}
