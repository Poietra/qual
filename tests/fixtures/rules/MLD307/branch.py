import time

from manim import *


class RebindScene(Scene):
    def construct(self):
        fake = None
        if fake is not None:
            # Rebinding the name anywhere in the file means `time` can no
            # longer be trusted to be the stdlib module: silence.
            time = fake
        label = DecimalNumber(0)
        label.add_updater(lambda m: m.set_value(time.time()))
        self.add(label)
        self.wait(1)
