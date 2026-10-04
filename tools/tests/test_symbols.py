import unittest

import gen_symbols as g

SAMPLE = """
namespace game {
using TransitionGameStateFn = void(__cdecl*)(int newState);
constexpr Fun<TransitionGameStateFn> kTransitionGameState{0x004c48e0};

using BeginNamedWindowFn = int(__cdecl*)(void* ctx, const char* name, float x, float y, float w,
                                         float h, unsigned int flags);
constexpr Fun<BeginNamedWindowFn> kBeginNamedWindow{0x005a9bf0};

constexpr Obj<uint32_t> kNetTurnCounter{0x00916210};
constexpr Obj<void> kSettingsTable{0x008cb498};
using HudPortraitFn = void(__cdecl*)(float x, float y, float w, float h, int view);
constexpr Obj<HudPortraitFn> kHudPortraitByType{0x008c9f48};
constexpr int kLobbyPasswordCapacity = 0x18;
}
"""


class ParseTests(unittest.TestCase):
    def test_parses_functions_and_objects_sorted_by_name(self):
        syms = g.parse(SAMPLE)
        self.assertEqual([s["name"] for s in syms],
                         ["BeginNamedWindow", "HudPortraitByType", "NetTurnCounter",
                          "SettingsTable", "TransitionGameState"])
        bnw = syms[0]
        self.assertEqual(bnw["kind"], "Function")
        self.assertEqual(bnw["static_addr"], 0x005A9BF0)
        self.assertEqual(bnw["c_type"], "int(__cdecl*)(void* ctx, const char* name, float x, "
                                        "float y, float w, float h, unsigned int flags)")
        self.assertEqual(syms[2], {"name": "NetTurnCounter", "kind": "Object",
                                   "static_addr": 0x00916210, "c_type": "uint32_t"})
        self.assertEqual(syms[1]["c_type"], "void(__cdecl*)(float x, float y, float w, float h, int view)")

    def test_undeclared_function_type_is_rejected(self):
        with self.assertRaises(g.BadHeader):
            g.parse("constexpr Fun<NopeFn> kNope{0x00401000};")

    def test_duplicate_name_is_rejected(self):
        with self.assertRaises(g.BadHeader):
            g.parse(SAMPLE + "constexpr Obj<int> kNetTurnCounter{0x00916214};")

    def test_address_outside_the_image_is_rejected(self):
        with self.assertRaises(g.BadHeader):
            g.parse("constexpr Obj<int> kFar{0x10000000};")


class EmitTests(unittest.TestCase):
    def test_emits_rust_table(self):
        rs = g.emit(g.parse(SAMPLE), "test")
        self.assertIn("pub const COUNT: usize = 5;", rs)
        self.assertIn('name: "BeginNamedWindow",', rs)
        self.assertIn("kind: Kind::Function,", rs)
        self.assertIn("kind: Kind::Object,", rs)
        self.assertIn("static_addr: 0x005a9bf0,", rs)
        self.assertTrue(rs.endswith("];\n"))


if __name__ == "__main__":
    unittest.main()
