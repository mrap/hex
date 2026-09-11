"""Private fake-signature fixtures; actual filesystem and CLI alias operations."""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SOURCE = Path(__file__).parents[1] / 'system/scripts/macos-app-install.py'
spec = importlib.util.spec_from_file_location('alias_installer', SOURCE)
INSTALL = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = INSTALL
spec.loader.exec_module(INSTALL)


@unittest.skipUnless(sys.platform == 'darwin', 'requires macOS atomic rename')
class AliasTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve()
        self.workspace = self.home / 'hex'
        self.bin = self.workspace / '.hex/bin'
        self.bin.mkdir(parents=True)
        self.root = self.home / '.codeintel'
        self.root.mkdir()
        self.env = patch.dict(os.environ, {'HOME': str(self.home)})
        self.env.start(); self.addCleanup(self.env.stop)
        self.product = getattr(self, 'product_name', 'code-intel-cli')
        self.name = INSTALL.PRODUCTS[self.product].executable
        identifier = INSTALL.PRODUCTS[self.product].bundle_identifier
        self.paths = INSTALL.product_paths(self.product, self.root)
        self.paths.executable.parent.mkdir(parents=True)
        self.paths.executable.write_text('#!/bin/sh\necho signed-fixture\n')
        self.paths.executable.chmod(0o755)
        (self.paths.app / 'Contents/Info.plist').write_bytes(b'fake public signature fixture')
        self.paths.cli.parent.mkdir()
        self.paths.cli.symlink_to(INSTALL._make_relative_cli_target(self.paths))
        self.policy = INSTALL.central_policy_path()
        self.policy.parent.mkdir(parents=True)
        self.policy.write_text(json.dumps({'schema_version': 1, 'certificate_sha1': 'A' * 40, 'team_id': 'TEAM123456'}))
        self.helpers = self.home / 'source/system/scripts'
        self.helpers.mkdir(parents=True)
        shutil.copyfile(SOURCE, self.helpers / SOURCE.name)
        shared = SOURCE.with_name('macos-signing.py').read_text().split('if __name__ == "__main__":')[0]
        self.verified = {'identifier': identifier, 'version': '0.1.0', 'team_id': 'TEAM123456', 'certificate_sha1': 'A' * 40, 'designated_requirements': {'arm64': 'fixture'}, 'mach_o_uuids': {'arm64': '11111111-1111-1111-1111-111111111111'}}
        fake = shared + "\nif __name__ == '__main__':\n    print(" + repr(json.dumps(self.verified)) + ")\n"
        (self.helpers / 'macos-signing.py').write_text(fake)
        self.paths.helper_dir.mkdir(parents=True)
        for name in ('macos-app-install.py', 'macos-signing.py'):
            shutil.copyfile(self.helpers / name, self.paths.helper_dir / name)
        state = dict(self.verified, schema_version=1, product=self.product, mode='signed-current', bundle_identifier=identifier, bundle_path=str(self.paths.app), executable_path=str(self.paths.executable), compatibility_path=str(self.paths.cli), generation='f' * 24, transaction_id='e' * 24, source_revision='a' * 40, bundle_sha256=INSTALL._tree_sha256(self.paths.app), executable_sha256=INSTALL._sha256(self.paths.executable), helpers={name: {'sha256': INSTALL._sha256(self.paths.helper_dir / name), 'source_revision': 'b' * 40} for name in ('macos-app-install.py', 'macos-signing.py')})
        self.paths.state.write_text(json.dumps(state))
        self.alias = self.bin / self.name
        self.old = b'#!/bin/sh\necho old-raw\n'
        self.alias.write_bytes(self.old); self.alias.chmod(0o751)
        verified = self.verified
        class FakeSigner:
            def verify_installed(self, *args): return dict(verified)
        self.signer = FakeSigner()

    def call(self, **kwargs):
        return INSTALL.compatibility_alias(self.product, self.root, self.workspace, self.signer, **kwargs)

    def cli(self, *args):
        return subprocess.run([sys.executable, '-I', '-B', str(self.helpers / SOURCE.name), 'compatibility-alias', self.product, '--root', str(self.root), '--hex-workspace', str(self.workspace), *args], env={'HOME': str(self.home), 'PATH': '/usr/bin:/bin'}, capture_output=True, text=True, timeout=10)

    def test_actual_cli_path_uses_signed_product_and_archives_old_bytes(self):
        before = subprocess.check_output([self.name], env={'PATH': str(self.bin) + ':/usr/bin:/bin'}, text=True)
        self.assertEqual(before, 'old-raw\n')
        result = self.cli()
        self.assertEqual(result.returncode, 0, result.stderr)
        value = json.loads(result.stdout)
        self.assertEqual(value['action'], 'migrated')
        self.assertTrue(value['published']); self.assertTrue(value['changed'])
        self.assertEqual(value['source_revision'], 'a' * 40)
        archive = Path(value['archive_path'])
        self.assertEqual((archive / ('previous-' + self.name)).read_bytes(), self.old)
        self.assertEqual(json.loads((archive / 'receipt.json').read_text())['previous']['mode'], 0o751)
        self.assertEqual(os.readlink(self.alias), str(self.paths.cli))
        self.assertEqual(subprocess.check_output([self.name], env={'PATH': str(self.bin) + ':/usr/bin:/bin'}, text=True), 'signed-fixture\n')

    def test_dry_run_leaves_tree_unchanged(self):
        before = self.alias.stat()
        result = self.cli('--dry-run')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['action'], 'would-migrate')
        after = self.alias.stat()
        self.assertEqual((after.st_ino, after.st_mode, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
                         (before.st_ino, before.st_mode, before.st_size, before.st_mtime_ns, before.st_ctime_ns))
        self.assertEqual(self.alias.read_bytes(), self.old)
        self.assertFalse((self.workspace / '.hex/.code-intel-compat-backups').exists())

    def test_correct_alias_is_noop_without_archive_or_inode_change(self):
        self.alias.unlink(); self.alias.symlink_to(self.paths.cli)
        before = self.alias.lstat()
        value = self.call()
        self.assertEqual(value['action'], 'current')
        self.assertFalse(value['changed']); self.assertFalse(value['published'])
        self.assertEqual(self.alias.lstat(), before)
        self.assertFalse((self.workspace / '.hex/.code-intel-compat-backups').exists())

    def test_missing_product_root_dry_run_creates_nothing(self):
        self.root.rename(self.home / 'saved-codeintel')
        result = self.cli('--dry-run')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.root.exists())
        self.assertEqual(self.alias.read_bytes(), self.old)

    def test_current_alias_rejects_parent_replacement_during_verification(self):
        self.alias.unlink(); self.alias.symlink_to(self.paths.cli)
        original = self.signer.verify_installed
        def replace_parent(*args):
            self.bin.rename(self.workspace / '.hex/old-bin')
            self.bin.mkdir()
            self.alias.write_bytes(b'actor replacement')
            return original(*args)
        with patch.object(self.signer, 'verify_installed', replace_parent):
            with self.assertRaises(INSTALL.InstallError): self.call()
        self.assertEqual(self.alias.read_bytes(), b'actor replacement')

    def test_missing_alias_is_created_without_archive(self):
        self.alias.unlink()
        value = self.call()
        self.assertEqual(value['action'], 'created')
        self.assertIsNone(value['archive_path'])
        self.assertEqual(os.readlink(self.alias), str(self.paths.cli))

    def test_foreign_alias_and_special_types_are_preserved(self):
        self.alias.unlink(); self.alias.symlink_to('/no/such/foreign')
        with self.assertRaises(INSTALL.InstallError): self.call()
        self.assertEqual(os.readlink(self.alias), '/no/such/foreign')
        self.alias.unlink(); os.mkfifo(self.alias)
        with self.assertRaises(INSTALL.InstallError): self.call()
        self.assertTrue(INSTALL.stat.S_ISFIFO(self.alias.lstat().st_mode))

    def test_wrong_root_and_aliased_workspace_fail_before_mutation(self):
        with self.assertRaises(INSTALL.InstallError):
            INSTALL.compatibility_alias(self.product, self.home / 'other', self.workspace, self.signer)
        alias = self.home / 'workspace-alias'; alias.symlink_to(self.workspace)
        with self.assertRaises(INSTALL.InstallError):
            INSTALL.compatibility_alias(self.product, self.root, alias, self.signer)
        self.assertEqual(self.alias.read_bytes(), self.old)

    def test_verification_failure_keeps_raw_file(self):
        self.policy.unlink()
        with self.assertRaises(INSTALL.InstallError): self.call()
        self.assertEqual(self.alias.read_bytes(), self.old)

    def test_product_lock_spans_verification_and_publication(self):
        original = INSTALL.service_owner
        observed = []
        def checked(*args, **kwargs):
            with self.assertRaisesRegex(INSTALL.InstallError, 'busy'):
                with INSTALL._product_lock(self.paths): pass
            observed.append(True)
            return original(*args, **kwargs)
        with patch.object(INSTALL, 'service_owner', side_effect=checked): self.call()
        self.assertEqual(observed, [True])

    def test_archive_failure_preserves_public_raw_file(self):
        with patch.object(INSTALL, '_alias_archive', side_effect=OSError('injected archive failure')):
            with self.assertRaises(INSTALL.InstallError) as error: self.call()
        self.assertFalse(error.exception.published)
        self.assertEqual(self.alias.read_bytes(), self.old)

    def test_actor_replacement_before_publication_is_preserved(self):
        original = INSTALL._alias_archive
        def replace(*args):
            result = original(*args)
            self.alias.unlink(); self.alias.write_bytes(b'actor replacement')
            return result
        with patch.object(INSTALL, '_alias_archive', side_effect=replace):
            with self.assertRaisesRegex(INSTALL.InstallError, 'changed'): self.call()
        self.assertEqual(self.alias.read_bytes(), b'actor replacement')

    def test_final_parent_sync_failure_reports_preserved_partial_publication(self):
        original = INSTALL._fsync_dir
        def fail_after_alias(fd):
            if self.alias.is_symlink(): raise OSError('injected post-publication sync failure')
            return original(fd)
        with patch.object(INSTALL, '_fsync_dir', side_effect=fail_after_alias):
            with self.assertRaises(INSTALL.InstallError) as error: self.call()
        self.assertTrue(error.exception.published)
        self.assertEqual(os.readlink(self.alias), str(self.paths.cli))
        self.assertEqual((Path(error.exception.result['archive_path']) / ('previous-' + self.name)).read_bytes(), self.old)
        self.assertEqual(self.call()['action'], 'current')

    def test_actual_archive_sync_failure_prevents_publication(self):
        with patch.object(INSTALL.os, 'fsync', side_effect=OSError('injected archive fsync failure')) as sync:
            with self.assertRaises(INSTALL.InstallError) as error: self.call()
        self.assertGreater(sync.call_count, 0)
        self.assertFalse(error.exception.published)
        self.assertEqual(self.alias.read_bytes(), self.old)

    def test_archive_copy_failure_prevents_publication(self):
        original = INSTALL.os.write
        def fail(fd, data):
            if bytes(data) == self.old: raise OSError('injected archive copy failure')
            return original(fd, data)
        with patch.object(INSTALL.os, 'write', side_effect=fail):
            with self.assertRaisesRegex(INSTALL.InstallError, 'archive copy'): self.call()
        self.assertEqual(self.alias.read_bytes(), self.old)

    def test_changed_destination_parent_preserves_actor_and_old_entry(self):
        original = INSTALL._alias_archive
        moved = self.bin.with_name('old-bin')
        def replace(*args):
            result = original(*args)
            self.bin.rename(moved); self.bin.mkdir()
            self.alias.write_bytes(b'actor parent entry')
            return result
        with patch.object(INSTALL, '_alias_archive', side_effect=replace):
            with self.assertRaisesRegex(INSTALL.InstallError, 'parent directory changed'): self.call()
        self.assertEqual(self.alias.read_bytes(), b'actor parent entry')
        self.assertEqual((moved / self.name).read_bytes(), self.old)

    def test_actual_cli_reports_post_publication_failure_as_partial(self):
        script = self.helpers / SOURCE.name
        injected = "\n_real_alias_sync = _fsync_dir\ndef _fsync_dir(fd):\n    if Path(" + repr(str(self.alias)) + ").is_symlink():\n        raise OSError('injected CLI final sync failure')\n    return _real_alias_sync(fd)\n"
        script.write_text(script.read_text().replace('# Rust service bootstraps', injected + '\n# Rust service bootstraps'))
        result = self.cli()
        self.assertEqual(result.returncode, 1, result.stdout)
        error = json.loads(result.stderr)
        self.assertTrue(error['published']); self.assertTrue(error['changed'])
        self.assertEqual(error['action'], 'migrated')
        self.assertEqual((Path(error['archive_path']) / ('previous-' + self.name)).read_bytes(), self.old)
        self.assertEqual(os.readlink(self.alias), str(self.paths.cli))



class DaemonAliasTests(AliasTests):
    product_name = 'code-intel-daemon'


if __name__ == '__main__': unittest.main(verbosity=2)
