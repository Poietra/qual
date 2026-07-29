from manim import *


class Demo(Scene):
    def construct(self):
        self.wait(stop_condition=lambda: True, frozen_frame=True)  # qual: ignore[MLC106]
