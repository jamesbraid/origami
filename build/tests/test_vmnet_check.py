import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "vmnet-check.py"
SPEC = importlib.util.spec_from_file_location("vmnet_check", MODULE_PATH)
assert SPEC and SPEC.loader
vmnet_check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(vmnet_check)

MAC = "08:00:69:55:5b:83"
CONSOLE = """>> boot -f bootp()192.168.105.1:/unix
Setting $netaddr to 192.168.105.107 (from server )
Obtaining /unix from server
Unable to load bootp()192.168.105.1:/unix: execute format error
"""
DNSMASQ = """dnsmasq-dhcp[3070]: 48040 BOOTP(bridge100) 192.168.105.107 08:00:69:55:5b:83
dnsmasq-tftp[3070]: sent /tmp/tftp/unix to 192.168.105.107
"""
CAPTURE = """21:33:44.179823 08:00:69:55:5b:83 > ff:ff:ff:ff:ff:ff, ethertype IPv4 (0x0800), length 342
    192.168.105.77.68 > 255.255.255.255.67: BOOTP/DHCP, Request from 08:00:69:55:5b:83, length 300
	  Client-Ethernet-Address 08:00:69:55:5b:83
21:33:47.209087 d6:c6:aa:e4:dd:64 > 08:00:69:55:5b:83, ethertype IPv4 (0x0800), length 342
    192.168.105.1.67 > 192.168.105.77.68: BOOTP/DHCP, Reply, length 300, xid 0xbba8
	  Client-IP 192.168.105.77
	  Your-IP 192.168.105.107
	  Server-IP 192.168.105.1
"""


class EvidenceTests(unittest.TestCase):
    def failures(self, console=CONSOLE, dnsmasq=DNSMASQ, capture=CAPTURE):
        return vmnet_check.evidence_failures(MAC, console, dnsmasq, capture)

    def test_a_complete_exchange_passes(self):
        self.assertEqual(self.failures(), [])

    def test_a_silent_network_fails(self):
        console = "No server for 192.168.105.1:/unix.\n"
        self.assertEqual(len(self.failures(console=console)), 1)

    def test_an_address_outside_the_pool_fails(self):
        console = CONSOLE.replace("192.168.105.107", "192.168.105.77")
        self.assertEqual(len(self.failures(console=console)), 1)

    def test_each_missing_proof_is_reported(self):
        self.assertEqual(len(self.failures(dnsmasq="")), 2)
        self.assertEqual(len(self.failures(capture="")), 2)
        self.assertEqual(len(self.failures(console=CONSOLE.replace("Obtaining", "Getting"))), 1)

    def test_another_clients_traffic_does_not_count(self):
        other = DNSMASQ.replace("08:00:69:55:5b:83", "08:00:69:00:00:01")
        self.assertEqual(len(self.failures(dnsmasq=other)), 1)


if __name__ == "__main__":
    unittest.main()
