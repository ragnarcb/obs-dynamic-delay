"""Run the real Lua bridge in LuaJIT with a small, file-backed OBS mock.

python -m pip install lupa==2.6
DD_TEST_BIN=target/release/obs-dynamic-delay python scripts/test_obs_bridge.py
The optional binary also verifies Lua journals with the real Rust uninstaller.
"""
import configparser
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

from lupa.luajit21 import LuaRuntime

ROOT = Path(__file__).resolve().parents[1]


class BridgeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.obs_dir = self.root / "obs-studio"
        self.app = self.root / "relay"
        self.app.mkdir()
        (self.obs_dir / "basic/scenes").mkdir(parents=True)
        self.denied = None
        self.messages = []
        self.profiles = {}
        self.select("A")
        self.lua = LuaRuntime(unpack_returned_tuples=True)
        obs = self.lua.table()
        self.lua.globals().obslua = obs
        self.lua.globals().script_path = lambda: self.app.as_posix() + "/"
        self.lua.globals().capture = self.messages.append
        self.lua.execute("""
          package.preload.ffi = function()
            return {os='Linux', cdef=function() end, new=function() return {} end,
              sizeof=function() return 16 end,
              C={socket=function() return 1 end, htons=function(x) return x end,
                 inet_addr=function() return 0 end,
                 sendto=function(_, s) capture(s); return #s end}}
          end
        """)
        obs.obs_data_create = dict
        obs.obs_data_create_from_json_file = self.read_json
        obs.obs_data_create_from_json_file_safe = lambda p, _: self.read_json(p)
        obs.obs_data_save_json_safe = lambda d, p, *_: self.save_json(d, p)
        obs.obs_data_release = lambda _: None
        obs.obs_data_get_obj = lambda d, k: copy.deepcopy(d.get(k))
        obs.obs_data_set_obj = lambda d, k, v: d.__setitem__(k, copy.deepcopy(v))
        obs.obs_data_has_user_value = lambda d, k: k in d and d[k] is not None
        obs.obs_data_apply = lambda d, src: d.update(copy.deepcopy(src))
        for kind, cast, default in [("int", int, 0), ("string", str, ""), ("bool", bool, False)]:
            obs["obs_data_get_" + kind] = lambda d, k, c=cast, v=default: c(d.get(k, v))
            obs["obs_data_set_" + kind] = lambda d, k, v, c=cast: d.__setitem__(k, c(v))
        obs.obs_frontend_get_current_profile_path = lambda: self.path.as_posix()
        obs.obs_frontend_get_profile_config = lambda: self.profile["config"]
        obs.config_has_user_value = lambda d, s, k: k in d.get(s, {})
        obs.config_get_string = lambda d, s, k: d.get(s, {}).get(k, "")
        obs.config_get_int = lambda d, s, k: int(d.get(s, {}).get(k, "0"))
        obs.config_get_bool = lambda d, s, k: d.get(s, {}).get(k, "false") == "true"
        obs.config_set_int = lambda d, s, k, v: d.setdefault(s, {}).__setitem__(k, str(v))
        obs.config_set_bool = lambda d, s, k, v: d.setdefault(s, {}).__setitem__(k, str(v).lower())
        obs.config_save_safe = lambda *_: self.save_config()
        obs.obs_frontend_streaming_active = lambda: False
        obs.obs_frontend_get_streaming_service = lambda: self.profile["service"]
        obs.obs_service_get_settings = lambda s: copy.deepcopy(s["settings"])
        obs.obs_service_get_type = lambda s: s["type"]
        obs.obs_service_create = lambda t, _, d, __: {"type": t, "settings": copy.deepcopy(d)}
        obs.obs_frontend_set_streaming_service = lambda s: self.profile.__setitem__("service", s)
        obs.obs_frontend_save_streaming_service = lambda: self.save_json(self.profile["service"], self.path / "service.json")
        obs.obs_service_release = lambda _: None
        obs.obs_encoder_defaults = lambda _: {"bitrate": 10000, "keyint_sec": 0}
        obs.obs_frontend_get_streaming_output = lambda: None
        obs.script_log = lambda *_: None
        obs.LOG_INFO = 200
        # Expose locals only inside this test chunk; no test hooks in shipped code.
        source = (ROOT / "obs/obs-dynamic-delay.lua").read_text(encoding="utf-8")
        self.bridge = self.lua.execute(source + "\nreturn {configure=configure_obs, restore=restore_obs, limits=apply_platform_limits}")

    @property
    def profile(self):
        return self.profiles[self.selected]

    @property
    def path(self):
        return self.obs_dir / "basic/profiles" / self.selected

    def select(self, name, advanced=False):
        self.selected = name
        if name not in self.profiles:
            self.path.mkdir(parents=True)
            self.profiles[name] = {
                "service": {"type": "rtmp_common", "settings": {"service": "Twitch", "server": "auto", "key": "key-" + name}},
                "config": {"Output": {"Mode": "Advanced" if advanced else "Simple", "DelayEnable": "true"},
                           "SimpleOutput": {"VBitrate": "9000"},
                           "AdvOut": {"Encoder": "test", "ApplyServiceSettings": "true"}},
            }
            self.save_config()
            self.save_json(self.profile["service"], self.path / "service.json")
        (self.obs_dir / "user.ini").write_text("[Basic]\nProfileDir=" + name + "\n", encoding="utf-8")

    @staticmethod
    def read_json(path):
        try:
            return json.loads(Path(path).read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None

    def save_json(self, data, path):
        if self.denied and str(path).endswith(self.denied):
            return False
        Path(path).write_text(json.dumps(data), encoding="utf-8")
        return True

    def save_config(self):
        cfg = configparser.ConfigParser()
        cfg.optionxform = str
        cfg.read_dict(self.profile["config"])
        with (self.path / "basic.ini").open("w", encoding="utf-8") as out:
            cfg.write(out, space_around_delimiters=False)
        return 0

    def test_profiles_keep_their_own_service_and_simple_settings(self):
        for name in ["A", "B", "C"]:
            self.select(name)
            self.bridge.configure()
            self.bridge.configure()  # must not replace the original with the relay
            self.assertEqual(self.read_json(self.path / "service.json.dd-backup")["settings"]["key"], "key-" + name)
            fields = self.read_json(self.path / "basic.ini.dd-changes.json")["fields"]
            self.assertEqual(fields["Output.DelayEnable"], {"before": "true", "applied": "false"})
            self.assertEqual(fields["SimpleOutput.VBitrate"], {"before": "9000", "applied": "6000"})
        self.assertFalse((self.app / "obs-service-backup.json").exists())
        self.select("B")
        self.bridge.restore()
        self.assertEqual(self.profile["service"]["settings"]["key"], "key-B")
        self.assertTrue((self.obs_dir / "basic/profiles/A/service.json.dd-backup").exists())
        self.assertTrue((self.path / "service.json.dd-backup").exists())

    def test_restore_legacy_backup_and_prefer_profile_backup(self):
        original = copy.deepcopy(self.profile["service"])
        self.save_json(original, self.app / "obs-service-backup.json")
        self.bridge.configure()
        profile_backup = self.path / "service.json.dd-backup"
        profile_backup.unlink()
        self.bridge.restore()
        self.assertEqual(self.profile["service"], original)
        self.assertTrue((self.app / "obs-service-backup.json").exists())
        self.bridge.configure()
        other = copy.deepcopy(original)
        other["settings"]["key"] = "wrong-legacy-profile"
        self.save_json(other, self.app / "obs-service-backup.json")
        self.bridge.restore()
        self.assertEqual(self.profile["service"], original)

    def test_failed_service_restore_keeps_backup(self):
        self.bridge.configure()
        self.lua.globals().obslua.obs_service_create = lambda *_: None
        self.bridge.restore()
        self.assertTrue((self.path / "service.json.dd-backup").exists())
        self.assertTrue(self.messages[-1].startswith("result error"))

    def test_failed_service_backup_prevents_reconfiguration(self):
        before = copy.deepcopy(self.profile)
        self.denied = "service.json.dd-backup"
        self.bridge.configure()
        self.assertEqual(self.profile, before)
        self.assertTrue(any(m.startswith("result error") for m in self.messages))

    def test_failed_journal_prevents_delay_and_service_changes(self):
        before = copy.deepcopy(self.profile)
        self.denied = "basic.ini.dd-changes.json"
        self.bridge.configure()
        self.assertEqual(self.profile, before)
        self.assertTrue((self.path / "service.json.dd-backup").exists())

    def test_advanced_tracks_only_changed_fields_and_missing_defaults(self):
        self.select("Advanced", advanced=True)
        encoder = self.path / "streamEncoder.json"
        self.save_json({"bitrate": 6000}, encoder)
        self.bridge.configure()
        fields = self.read_json(str(encoder) + ".dd-changes.json")["fields"]
        self.assertEqual(fields, {"keyint_sec": {"applied": 2}})
        self.assertEqual(self.read_json(encoder), {"bitrate": 6000, "keyint_sec": 2})

    def test_advanced_backup_failure_does_not_change_encoder(self):
        self.select("Advanced", advanced=True)
        encoder = self.path / "streamEncoder.json"
        self.save_json({"bitrate": 10000, "keyint_sec": 4}, encoder)
        self.denied = "streamEncoder.json.dd-changes.json"
        self.bridge.limits("Twitch")
        self.assertEqual(self.read_json(encoder), {"bitrate": 10000, "keyint_sec": 4})

    def test_repeated_caps_rebase_after_manual_edit(self):
        self.select("Advanced", advanced=True)
        encoder = self.path / "streamEncoder.json"
        self.save_json({"bitrate": 10000, "keyint_sec": 0}, encoder)
        self.bridge.limits("Kick")
        self.bridge.limits("Twitch")
        journal = Path(str(encoder) + ".dd-changes.json")
        self.assertEqual(self.read_json(journal)["fields"]["bitrate"]["before"], 10000)
        self.save_json({"bitrate": 9500, "keyint_sec": 2}, encoder)
        self.bridge.limits("Twitch")
        self.assertEqual(self.read_json(journal)["fields"]["bitrate"]["before"], 9500)

    @unittest.skipUnless(os.environ.get("DD_TEST_BIN") and sys.platform == "linux", "Linux helper process check")
    def test_helper_refuses_to_uninstall_while_obs_is_running(self):
        self.bridge.configure()
        scene = self.obs_dir / "basic/scenes/Main.json"
        original = {"modules": {"scripts-tool": [{"path": (self.app / "obs-dynamic-delay.lua").as_posix()}]}}
        self.save_json(original, scene)
        executable = self.root / "obs"
        shutil.copy2(shutil.which("sleep"), executable)
        process = subprocess.Popen([str(executable), "30"])
        try:
            time.sleep(0.1)
            env = dict(os.environ, DD_OBS_CONFIG_DIR=str(self.obs_dir), DD_INSTALL_DIR=str(self.app))
            env.pop("DD_SKIP_OBS_CHECK", None)
            result = subprocess.run([str(Path(os.environ["DD_TEST_BIN"]).resolve()), "--uninstall", "--quiet"],
                                    env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(self.read_json(scene), original)
            self.assertTrue((self.path / "service.json.dd-backup").exists())
            self.assertTrue((self.app / "setup.log").exists())
        finally:
            process.terminate()
            process.wait()

    @unittest.skipUnless(os.environ.get("DD_TEST_BIN"), "set DD_TEST_BIN for Rust/Lua integration")
    def test_rust_uninstall_restores_all_lua_profiles_and_preserves_manual_edits(self):
        for name in ["A", "B", "C"]:
            self.select(name)
            self.bridge.configure()
        self.select("Advanced", advanced=True)
        encoder = self.path / "streamEncoder.json"
        self.save_json({"bitrate": 6000}, encoder)
        self.bridge.configure()
        self.save_json({"bitrate": 8000, "keyint_sec": 2}, encoder)
        self.select("B")
        self.profile["config"]["SimpleOutput"]["VBitrate"] = "7000"
        self.save_config()
        env = dict(os.environ, DD_OBS_CONFIG_DIR=str(self.obs_dir), DD_INSTALL_DIR=str(self.app))
        binary = str(Path(os.environ["DD_TEST_BIN"]).resolve())
        result = subprocess.run([binary, "--uninstall", "--quiet"], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for name in ["A", "B", "C", "Advanced"]:
            self.select(name)
            self.assertEqual(self.read_json(self.path / "service.json")["settings"]["key"], "key-" + name)
            restored = (self.path / "basic.ini").read_text()
            self.assertIn("DelayEnable=true", restored)
            self.assertIn("VBitrate=" + ("7000" if name == "B" else "9000"), restored)
        self.assertEqual(self.read_json(encoder), {"bitrate": 8000})
        self.assertFalse(list(self.obs_dir.rglob("*.dd-changes.json")))


if __name__ == "__main__":
    unittest.main()
