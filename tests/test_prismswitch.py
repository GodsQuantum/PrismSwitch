import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("ps", Path(__file__).parents[1] / "prismswitch/prismswitch.py")
ps = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ps)

class ConfigTests(unittest.TestCase):
    def setUp(self):
        self.c = copy.deepcopy(ps.DEFAULT)

    def test_defaults_and_example(self):
        self.assertEqual(ps.validate(self.c), self.c)
        self.assertEqual(json.loads((Path(__file__).parents[1] / "config.example.json").read_text()), self.c)

    def test_keyd_normal_and_eco(self):
        config = ps.keyd_config(self.c)
        self.assertIn("[control+alt]", config)
        self.assertIn("m = command(/usr/local/bin/prismswitch toggle)", config)
        self.assertIn("e = command(/usr/local/bin/prismswitch profile eco)", config)

    def test_custom_multiple_profiles(self):
        self.c["profiles"]["quiet"] = {"cpu_percent": 30, "disable_boost": True, "pause_services": []}
        self.c["shortcuts"]["quiet"] = "Super+Shift+F6"
        self.assertIn("[shift+meta]", ps.keyd_config(self.c))
        self.assertIn("f6 = command(/usr/local/bin/prismswitch profile quiet)", ps.keyd_config(self.c))

    def test_duplicate_alias(self):
        self.c["shortcuts"]["eco"] = "Alt+Control+M"
        with self.assertRaises(ps.Error):
            ps.validate(self.c)

    def test_reject_injection(self):
        for key in ("Ctrl+Alt+M;rm", "Ctrl+Alt+$(echo x)", "Ctrl+M", "Ctrl+Ctrl+M", "Ctrl+Alt+F13"):
            with self.subTest(key=key), self.assertRaises(ps.Error):
                ps.chord(key)

    def test_reject_profile_injection(self):
        self.c["profiles"]["x;echo"] = self.c["profiles"]["eco"]
        with self.assertRaises(ps.Error):
            ps.validate(self.c)

    def test_reject_bad_service(self):
        self.c["user"] = "demo"
        self.c["profiles"]["eco"]["pause_services"] = ["--user"]
        with self.assertRaises(ps.Error):
            ps.validate(self.c)

    def test_reject_unowned_services(self):
        self.c["profiles"]["eco"]["pause_services"] = ["sync.service"]
        with self.assertRaises(ps.Error):
            ps.validate(self.c)

    def test_reject_boolean_cpu(self):
        self.c["profiles"]["eco"]["cpu_percent"] = True
        with self.assertRaises(ps.Error):
            ps.validate(self.c)

    def test_frequency_round_down(self):
        self.assertEqual(ps.cpu_cap(1200, 2660, 0, [1200, 1600, 2000, 2660]), 1200)
        self.assertEqual(ps.cpu_cap(1200, 2660, 50, [1200, 1600, 2000, 2660]), 1600)
        self.assertEqual(ps.cpu_cap(1200, 2660, 100, [1200, 1600, 2000, 2660]), 2660)
        self.assertEqual(ps.cpu_cap(1200, 2660, 50), 1930)

class FakeHardware(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.sys = self.root / "sys"
        self.cpu = self.sys / "devices/system/cpu/cpufreq"
        self.state = self.root / "run/eco.json"
        self.c = copy.deepcopy(ps.DEFAULT)
        self.write(self.cpu / "boost", "1")
        for i in range(2):
            for name, val in {"scaling_min_freq": "1200", "scaling_max_freq": "2660", "scaling_governor": "schedutil", "scaling_available_governors": "schedutil powersave", "scaling_available_frequencies": "1200 1600 2000 2660"}.items():
                self.write(self.cpu / f"policy{i}" / name, val)
        self.addCleanup(patch.stopall)
        patch.object(ps, "SYS", self.sys).start()
        patch.object(ps, "STATE", self.state).start()
        patch.object(ps.time, "sleep").start()
        self.run = patch.object(ps, "run", return_value="PrismSwitch sleep inhibitor").start()
        self.success = patch.object(ps, "success", return_value=True).start()

    def write(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value)

    def test_apply_restore(self):
        ps.apply_profile(self.c, "eco")
        self.assertTrue(json.loads(self.state.read_text())["active"])
        self.assertEqual(ps.read(self.cpu / "boost"), "0")
        self.assertEqual(ps.read(self.cpu / "policy0/scaling_max_freq"), "1200")
        self.assertEqual(ps.read(self.cpu / "policy1/scaling_governor"), "powersave")
        ps.restore()
        self.assertFalse(self.state.exists())
        self.assertEqual(ps.read(self.cpu / "boost"), "1")
        self.assertEqual(ps.read(self.cpu / "policy0/scaling_max_freq"), "2660")
        self.assertEqual(ps.read(self.cpu / "policy1/scaling_governor"), "schedutil")

    def test_transaction_failure_restores(self):
        original = ps.set_checked
        def fail(path, value):
            if path.name == "scaling_governor" and value == "powersave":
                raise OSError("simulated write error")
            original(path, value)
        with patch.object(ps, "set_checked", side_effect=fail):
            with self.assertRaises(OSError):
                ps.apply_profile(self.c, "eco")
        self.assertFalse(self.state.exists())
        self.assertEqual(ps.read(self.cpu / "boost"), "1")
        self.assertEqual(ps.read(self.cpu / "policy0/scaling_max_freq"), "2660")

    def test_restore_keeps_recovery_state_on_failure(self):
        ps.apply_profile(self.c, "eco")
        with patch.object(ps, "set_checked", side_effect=OSError("simulated restoration failure")):
            with self.assertRaises(ps.Error):
                ps.restore()
        self.assertTrue(self.state.exists())
        ps.restore()
        self.assertFalse(self.state.exists())

    def test_unsupported_model_no_smc_write(self):
        self.write(self.sys / "class/dmi/id/product_name", "iMac18,3")
        with self.assertRaises(ps.Error):
            ps.device(self.c)

    def test_native_read_restores_selector(self):
        dev = self.root / "device"
        for name, val in {"key_at_index": "4", "key_count": "5", "key_at_index_name": "MVHR"}.items():
            self.write(dev / name, val)
        (dev / "key_at_index_data").write_bytes(b"\x01")
        self.assertEqual(ps.mode_native(dev), "on")
        self.assertEqual(ps.read(dev / "key_at_index"), "4")

    def test_native_bad_value_restores_selector(self):
        dev = self.root / "device"
        for name, val in {"key_at_index": "4", "key_count": "5", "key_at_index_name": "MVHR"}.items():
            self.write(dev / name, val)
        (dev / "key_at_index_data").write_bytes(b"\x02")
        with self.assertRaises(ps.Error):
            ps.mode_native(dev)
        self.assertEqual(ps.read(dev / "key_at_index"), "4")

    def test_smc_rebind_on_failed_route(self):
        driver = self.root / "driver"
        driver.mkdir()
        (driver / "unbind").touch()
        (driver / "bind").touch()
        dev = driver / "applesmc.768"
        dev.mkdir()
        with patch.object(ps, "DRIVER", driver), patch.object(ps.os.path, "exists", return_value=True), patch.object(ps, "smc", side_effect=RuntimeError("injected fault")), patch.object(Path, "exists", return_value=False):
            with self.assertRaises(ps.Error):
                ps.route(dev, "on")
        self.assertEqual((driver / "unbind").read_text(), "applesmc.768")
        self.assertEqual((driver / "bind").read_text(), "applesmc.768")

if __name__ == "__main__":
    unittest.main()
