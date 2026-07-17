from manim import *


def make():
    return None


class Demo(Scene):
    def construct(self):
        thing = make()
        self.play(thing.animate.copy())
