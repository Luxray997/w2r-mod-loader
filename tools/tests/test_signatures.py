import unittest

import gen_signatures as g

SAMPLE = """
// SPDX-License-Identifier: MIT
const uint8_t kSigAlpha[] = {
    0x55, 0x8b, 0xec, 0x66, 0x8b, 0x0d, 0x84, 0xc1,
    0x91, 0x00,
};
const RelocSlot kRelAlpha[] = {
    {6, 0x0091c184},
};

const uint8_t kSigBeta[] = {
    0xe8, 0x1b, 0x6d, 0xff,
};

const Signature kSignatures[] = {
    {"alpha \\"quoted\\"", 0x004c48e0, kSigAlpha, sizeof(kSigAlpha), kRelAlpha,
     sizeof(kRelAlpha) / sizeof(RelocSlot)},
    {"beta", 0x0049ea20, kSigBeta, sizeof(kSigBeta), nullptr, 0},
};
"""


class ParseTests(unittest.TestCase):
    def test_parses_bytes_relocs_and_entries(self):
        sigs = g.parse(SAMPLE)
        self.assertEqual([s["what"] for s in sigs], ['alpha "quoted"', "beta"])
        self.assertEqual(sigs[0]["static_addr"], 0x004C48E0)
        self.assertEqual(sigs[0]["bytes"], bytes.fromhex("558bec668b0d84c19100"))
        self.assertEqual(sigs[0]["relocs"], [(6, 0x0091C184)])
        self.assertEqual(sigs[1]["bytes"], bytes.fromhex("e81b6dff"))
        self.assertEqual(sigs[1]["relocs"], [])

    def test_reloc_past_the_end_is_rejected(self):
        bad = SAMPLE.replace("{6, 0x0091c184}", "{7, 0x0091c184}")
        with self.assertRaises(g.BadHeader):
            g.parse(bad)

    def test_undefined_table_is_rejected(self):
        bad = SAMPLE.replace("kSigBeta, sizeof(kSigBeta)", "kSigGamma, sizeof(kSigGamma)")
        with self.assertRaises(g.BadHeader):
            g.parse(bad)

    def test_unused_byte_table_is_rejected(self):
        bad = SAMPLE + "\nconst uint8_t kSigOrphan[] = { 0x90 };\n"
        with self.assertRaises(g.BadHeader):
            g.parse(bad)


class EmitTests(unittest.TestCase):
    def test_emits_rust_table(self):
        rs = g.emit(g.parse(SAMPLE), "test")
        self.assertIn("pub const COUNT: usize = 2;", rs)
        self.assertIn('what: "alpha \\"quoted\\"",', rs)
        self.assertIn("static_addr: 0x004c48e0,", rs)
        self.assertIn("RelocSlot { offset: 6, static_addr: 0x0091c184 },", rs)
        self.assertIn("0x55, 0x8b, 0xec,", rs)
        self.assertIn("relocs: &[],", rs)
        self.assertTrue(rs.endswith("];\n"))


if __name__ == "__main__":
    unittest.main()
