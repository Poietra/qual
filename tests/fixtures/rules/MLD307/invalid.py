import socket
import time
from datetime import datetime
from pathlib import Path

import requests
from manim import *


class ClockScene(Scene):
    def construct(self):
        label = DecimalNumber(0)
        label.add_updater(lambda m: m.set_value(time.time()))
        label.add_updater(lambda m: m.set_value(datetime.now().timestamp()))
        label.add_updater(lambda m: m.set_value(float(Path("data.txt").read_text())))
        label.add_updater(lambda m: m.set_value(len(open("data.txt").read())))
        label.add_updater(lambda m: m.set_value(len(requests.get("http://example.com").text)))
        label.add_updater(lambda m: m.set_value(len(socket.gethostbyname("example.com"))))
        self.add(label)
        self.wait(2)
