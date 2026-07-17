from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        if square.submobjects:
            square.add_updater(lambda m: m.set_color(RED))
        self.wait(3, frozen_frame=False)
