from manim import *


def cold_helper(scene):
    scene.wait(1)


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(lambda m: m.shift(RIGHT))
        self.wait(1)
        cold_helper(self)
