import importlib.util
import json
import shutil
import sys
import tempfile
import re
import unittest
from pathlib import Path


SOURCE = Path(__file__).parents[1] / "system/scripts/macos-app-install.py"
spec = importlib.util.spec_from_file_location("macos_app_install_products", SOURCE)
INSTALL = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = INSTALL
spec.loader.exec_module(INSTALL)
SIGN_SOURCE = Path(__file__).parents[1] / "system/scripts/macos-signing.py"
sign_spec = importlib.util.spec_from_file_location("macos_signing_products", SIGN_SOURCE)
SIGN = importlib.util.module_from_spec(sign_spec)
sys.modules[sign_spec.name] = SIGN
sign_spec.loader.exec_module(SIGN)
SIGN_TEST_SOURCE = Path(__file__).with_name("test_macos_signing.py")
sign_test_spec = importlib.util.spec_from_file_location("macos_signing_product_runner", SIGN_TEST_SOURCE)
SIGN_TEST = importlib.util.module_from_spec(sign_test_spec)
sys.modules[sign_test_spec.name] = SIGN_TEST
sign_test_spec.loader.exec_module(SIGN_TEST)


class ProductContractTests(unittest.TestCase):
    def test_signer_and_installer_agree_on_every_supported_product(self):
        self.assertEqual(set(INSTALL.PRODUCTS), set(SIGN.PRODUCTS))
        for name, product in INSTALL.PRODUCTS.items():
            with self.subTest(product=name):
                signer = SIGN.PRODUCTS[name]
                self.assertEqual(product.bundle_identifier, signer["identifier"])
                self.assertEqual(product.executable, signer["executable"])
                self.assertEqual(product.app_name, signer["bundle"])


class FakeSigner:
    def __init__(self, version):
        self.calls = []
        self.version = version

    def _result(self, product, version):
        return {
            "identifier": INSTALL.PRODUCTS[product].bundle_identifier,
            "version": version,
            "team_id": "TEAM123456",
            "certificate_sha1": "A" * 40,
            "designated_requirements": {"arm64": "anchor apple generic"},
            "mach_o_uuids": {"arm64": "11111111-1111-1111-1111-111111111111"},
        }

    def stage(self, source, product, policy, candidate, receipt):
        self.calls.append(("stage", product, self.version))
        item = INSTALL.PRODUCTS[product]
        executable = candidate / "Contents/MacOS" / item.executable
        executable.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, executable)
        executable.chmod(0o755)
        (candidate / "Contents/Info.plist").write_bytes(b"fake plist")
        result = self._result(product, self.version)
        receipt.write_text(json.dumps(result), encoding="utf-8")
        return result

    def verify_installed(self, bundle, product, policy, expected=None):
        self.calls.append(("verify", product, self.version))
        return self._result(product, self.version)


@unittest.skipUnless(sys.platform == "darwin", "requires macOS renameatx_np")
class CodeIntelProductTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.base = Path(self.temp.name)
        cargo = (Path(__file__).parents[1] / "system/code-intel/Cargo.toml").read_text(encoding="utf-8")
        self.codeintel_version = re.search(r'^version\s*=\s*"([^"]+)"\s*$', cargo, re.MULTILINE).group(1)
        self.root = self.base / ".codeintel"
        self.policy = self.base / "policy.json"
        self.policy.write_text(json.dumps({"schema_version": 1, "certificate_sha1": "A" * 40, "team_id": "TEAM123456"}), encoding="utf-8")
        self.source = self.base / "code-intel-source"
        self.source.write_text(f"code-intel {self.codeintel_version}", encoding="utf-8")
        self.helpers = {}
        self.sources = {}
        for name, value in (("macos-signing.py", b"signer"), ("macos-app-install.py", b"installer")):
            path = self.base / name
            path.write_bytes(value)
            self.sources[name] = path
            self.helpers[name] = {"sha256": INSTALL._sha256(path), "source_revision": "f" * 40}
        self.signer = FakeSigner(self.codeintel_version)
        self.addCleanup(self.temp.cleanup)

    def install(self, product):
        return INSTALL.install(product, self.root, self.source, self.signer, policy_path=self.policy, helper_provenance=self.helpers, helper_sources=self.sources, source_revision="e" * 40, version=self.codeintel_version)

    def test_product_map_and_manifest_version_are_fixed(self):
        cargo = (Path(__file__).parents[1] / "system/code-intel/Cargo.toml").read_text(encoding="utf-8")
        version = re.search(r'^version\s*=\s*"([^"]+)"\s*$', cargo, re.MULTILINE).group(1)
        daemon = INSTALL.product_paths("code-intel-daemon", self.root)
        cli = INSTALL.product_paths("code-intel-cli", self.root)
        self.assertEqual(daemon.app.name, "SCIPD.app")
        self.assertEqual(daemon.executable.name, "scipd")
        self.assertEqual(daemon.cli, self.root / "bin/scipd")
        self.assertEqual(daemon.helper_dir, self.root / "libexec/scipd")
        self.assertEqual(cli.app.name, "CQ.app")
        self.assertEqual(cli.executable.name, "cq")
        self.assertEqual(cli.cli, self.root / "bin/cq")
        self.assertEqual(cli.helper_dir, self.root / "libexec/cq")
        self.assertNotEqual(daemon.helper_dir, cli.helper_dir)
        self.install("code-intel-daemon")
        self.install("code-intel-cli")
        self.assertEqual([call[2] for call in self.signer.calls], [version] * 4)

    def test_standalone_signer_stages_and_verifies_code_intel_products(self):
        signer_policy = self.base / "signer-policy.json"
        signer_policy.write_text(json.dumps({"schema_version": 1, "certificate_sha1": SIGN_TEST.FINGERPRINT, "team_id": SIGN_TEST.TEAM}), encoding="utf-8")
        for product, identity in (("code-intel-daemon", "hex.scipd"), ("code-intel-cli", "hex.cq")):
            output = self.base / f"{product}.app"
            runner = SIGN_TEST.Runner(identity)
            result = SIGN.stage(self.source, product, signer_policy, output, self.codeintel_version, run=runner)
            self.assertEqual(result["identifier"], "com.mrap." + identity)
            self.assertEqual(result["version"], self.codeintel_version)
            readonly = SIGN_TEST.Runner(identity)
            readonly.signed = True
            verified = SIGN.verify_installed(output, product, signer_policy, run=readonly)
            self.assertTrue(verified["verified"])
            self.assertEqual(verified["identifier"], result["identifier"])

    def test_product_update_preserves_other_product_state_and_helpers(self):
        self.install("code-intel-daemon")
        self.install("code-intel-cli")
        daemon = INSTALL.product_paths("code-intel-daemon", self.root)
        cli = INSTALL.product_paths("code-intel-cli", self.root)
        before = (INSTALL._tree_sha256(daemon.app), daemon.state.read_bytes(), INSTALL._sha256(daemon.helper_dir / "macos-app-install.py"))
        self.source.write_text(f"code-intel {self.codeintel_version} updated", encoding="utf-8")
        changed_helper = self.base / "macos-app-install-updated.py"
        changed_helper.write_bytes(b"installer updated")
        updated_sources = dict(self.sources)
        updated_sources["macos-app-install.py"] = changed_helper
        updated_helpers = dict(self.helpers)
        updated_helpers["macos-app-install.py"] = {"sha256": INSTALL._sha256(changed_helper), "source_revision": "a" * 40}
        INSTALL.install("code-intel-cli", self.root, self.source, self.signer, policy_path=self.policy, helper_provenance=updated_helpers, helper_sources=updated_sources, source_revision="b" * 40, version=self.codeintel_version)
        after = (INSTALL._tree_sha256(daemon.app), daemon.state.read_bytes(), INSTALL._sha256(daemon.helper_dir / "macos-app-install.py"))
        self.assertEqual(before, after)
        self.assertTrue(cli.app.is_dir())
        self.assertTrue(cli.cli.is_symlink())

    def test_helper_directory_symlink_is_rejected(self):
        paths = INSTALL.product_paths("code-intel-cli", self.root)
        paths.root.mkdir(parents=True)
        outside = self.base / "outside"
        outside.mkdir()
        paths.helper_dir.parent.mkdir(parents=True)
        paths.helper_dir.symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(INSTALL.InstallError, "aliased"):
            self.install("code-intel-cli")

    def test_helper_parent_symlink_is_rejected(self):
        paths = INSTALL.product_paths("code-intel-daemon", self.root)
        paths.root.mkdir(parents=True)
        outside = self.base / "outside-parent"
        outside.mkdir()
        (paths.root / "libexec").symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(INSTALL.InstallError, "aliased"):
            self.install("code-intel-daemon")


if __name__ == "__main__":
    unittest.main()
