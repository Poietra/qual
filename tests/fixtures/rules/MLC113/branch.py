from manim import *


def make():
    return None


class Demo(Scene):
    def construct(self):
        make().animate.shift(RIGHT)(run_time=2)
