"""Keep the macOS engine's linker note usable with colored CI diagnostics."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from build_engine import native_link_flags


class NativeLinkFlagsTests(unittest.TestCase):
    def test_plain_and_colored_rustc_notes_preserve_frameworks_and_libraries(self):
        flags = '-framework ApplicationServices -framework CoreFoundation -lc++ -lSystem'
        self.assertEqual(native_link_flags('note: native-static-libs: ' + flags + '\n'), flags)
        self.assertEqual(native_link_flags(
            '\x1b[1m\x1b[92mnote\x1b[0m\x1b[1m: native-static-libs: ' + flags + '\x1b[0m\n'), flags)

    def test_other_diagnostics_are_not_linker_notes(self):
        self.assertIsNone(native_link_flags('warning: native-static-libs: ignored'))
        self.assertIsNone(native_link_flags('Finished release profile'))


if __name__ == '__main__':
    unittest.main()
