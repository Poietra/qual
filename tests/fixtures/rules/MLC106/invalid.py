from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.wait(stop_condition=lambda: True, frozen_frame=True)
