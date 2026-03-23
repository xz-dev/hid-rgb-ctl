from importlib.metadata import version

__version__ = version("hid-rgb-ctl")

from hid_rgb_ctl.descriptor import LampArrayInfo, LedRgbInfo, discover_devices
from hid_rgb_ctl.device import LampArrayDevice, LedRgbDevice

__all__ = [
    "discover_devices",
    "LampArrayInfo",
    "LedRgbInfo",
    "LampArrayDevice",
    "LedRgbDevice",
]
