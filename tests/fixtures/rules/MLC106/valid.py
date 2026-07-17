from manim import *


class Demo(Scene):
    def construct(self):
        self.wait(stop_condition=lambda: True)
        self.wait(frozen_frame=True)
        self.wait(stop_condition=None, frozen_frame=True)
        self.wait(stop_condition=lambda: True, frozen_frame=False)
