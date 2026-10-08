"""Architecture checks apply to test builds as well as application builds."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "architecture", Path(__file__).with_name("check-architecture.py")
)
architecture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(architecture)


class ArchitectureTests(unittest.TestCase):
    def test_tui_test_build_cannot_depend_on_service_or_store(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            members = list(architecture.DEPENDENCIES)
            (root / "Cargo.toml").write_text(
                "[workspace]\nmembers = " + repr(members) + "\n"
            )
            for name in members:
                (root / name).mkdir()
                (root / name / "Cargo.toml").write_text(
                    f'[package]\nname = "{name}"\n'
                )
            manifest = root / "orbit-tui/Cargo.toml"
            with patch.object(architecture, "ROOT", root):
                for dependency in ["orbit-client", "orbit-protocol"]:
                    manifest.write_text(
                        '[package]\nname = "orbit-tui"\n'
                        f'[dev-dependencies]\n{dependency} = "*"\n'
                    )
                    self.assertEqual(architecture.check(), [])
                for dependency in ["orbit-service", "orbit-store"]:
                    with self.subTest(dependency=dependency):
                        manifest.write_text(
                            '[package]\nname = "orbit-tui"\n'
                            f'[dev-dependencies]\n{dependency} = "*"\n'
                        )
                        errors = architecture.check()
                        self.assertEqual(len(errors), 1)
                        self.assertIn(dependency, errors[0])
                        self.assertIn("orbit-tui", errors[0])


if __name__ == "__main__":
    unittest.main()
