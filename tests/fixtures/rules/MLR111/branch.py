from manim import *
from helpers import external_tick


class UnknownScene(Scene):
    def construct(self):
        dot = Dot()
        self.add(dot)
        # The callback body cannot be resolved: silence.
        self.add_updater(external_tick)
        self.wait(2)
