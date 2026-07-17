from manim import *


def build_target(mob):
    return mob


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(ApplyMethod(square.shift, RIGHT))
        self.play(ApplyMethod(build_target(square), RIGHT))
