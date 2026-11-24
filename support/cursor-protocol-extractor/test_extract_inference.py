import unittest

from extract_inference import extract, generate


class CompactDescriptorTests(unittest.TestCase):
    def test_minified_helper_names_do_not_change_the_wire_contract(self):
        for base, helper in [("xe", "Re"), ("_e", "ve")]:
            with self.subTest(base=base, helper=helper):
                source = (
                    f'var ns="aiserver.v1.",Base=class extends {base}'
                    '{static $p(){return ns}},'
                    f'Kind={helper}(l,ns,"Kind",[[0,"UNSPECIFIED"],[1,"VALUE"]],1),'
                    'Msg=class t extends Base{static $(){return['
                    '"Example|1 kind #0|7 context_input_tokens 5?",Kind]}}'
                )
                schema = generate(extract(source), ["aiserver.v1.Example"])
                self.assertIn("optional int32 context_input_tokens = 7;", schema)
                self.assertIn("KIND_VALUE = 1;", schema)


if __name__ == "__main__":
    unittest.main()
