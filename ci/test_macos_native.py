import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from prepare_macos_native import prepare


class MacOSSoftwareFallbackDependency(unittest.TestCase):
    def check_manifest(self, dependencies):
        with tempfile.TemporaryDirectory() as temporary:
            tree = Path(temporary)
            port = tree / "res/vcpkg/ffmpeg"
            port.mkdir(parents=True)
            (port / "portfile.cmake").write_text('if(VCPKG_TARGET_IS_WINDOWS)\nelseif(VCPKG_TARGET_IS_OSX)\nendif()\n')
            manifest = port / "vcpkg.json"
            manifest.write_text(json.dumps({"name": "ffmpeg", "dependencies": dependencies}))
            prepare(tree)
            first = manifest.read_bytes()
            first_port = (port / "portfile.cmake").read_bytes()
            prepare(tree)
            self.assertEqual(manifest.read_bytes(), first)
            self.assertEqual((port / "portfile.cmake").read_bytes(), first_port)
            self.assertIn(b"--enable-libx264", first_port)
            return json.loads(first)["dependencies"]

    def test_fresh_pinned_port_declares_the_encoder_it_enables(self):
        dependencies = self.check_manifest([{"name": "vcpkg-cmake-get-vars", "host": True}])
        self.assertIn({"name": "x264", "platform": "osx"}, dependencies)

    def test_existing_windows_dependency_keeps_its_platform(self):
        dependencies = self.check_manifest([{"name": "x264", "platform": "windows & !uwp"}])
        self.assertEqual(dependencies, [{"name": "x264", "platform": "osx | (windows & !uwp)"}])

    def test_unconditional_dependency_is_not_duplicated(self):
        self.assertEqual(self.check_manifest(["x264"]), ["x264"])


if __name__ == "__main__":
    unittest.main()
