import importlib.util
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "remote_bundle", Path(__file__).with_name("test-remote-bundle.py")
)
remote_bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(remote_bundle)


class SystemCaBundleTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bundle = self.root / "ca-certificates.crt"
        self.bundle.write_text("test certificate bundle")
        self.enterContext(patch.dict(os.environ, {}, clear=True))
        self.enterContext(patch.object(remote_bundle, "SYSTEM_CA_BUNDLES", (self.bundle,)))
        self.paths = self.enterContext(patch.object(remote_bundle.ssl, "get_default_verify_paths"))
        self.paths.return_value = SimpleNamespace(cafile=None)

    def test_missing_python_default_uses_system_bundle(self):
        self.assertEqual(remote_bundle.system_ca_bundle(), self.bundle)

    def test_existing_python_default_takes_precedence(self):
        default = self.root / "python-ca.pem"
        default.write_text("Python certificate bundle")
        self.paths.return_value = SimpleNamespace(cafile=str(default))
        self.assertEqual(remote_bundle.system_ca_bundle(), default)

    def test_explicit_bundle_resolves_symlinks(self):
        explicit = self.root / "custom-ca.pem"
        explicit.write_text("custom certificate bundle")
        link = self.root / "ca-link.pem"
        link.symlink_to(explicit)
        with patch.dict(os.environ, {"SSL_CERT_FILE": str(link)}):
            self.assertEqual(remote_bundle.system_ca_bundle(), explicit)

    def test_missing_or_empty_bundles_report_how_to_supply_certificates(self):
        self.bundle.write_text("")
        self.paths.return_value = SimpleNamespace(cafile=str(self.root / "missing.pem"))
        with self.assertRaisesRegex(RuntimeError, "install ca-certificates or set SSL_CERT_FILE"):
            remote_bundle.system_ca_bundle()


if __name__ == "__main__":
    unittest.main()
