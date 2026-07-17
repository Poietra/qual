from manim import *


def make_dot():
    return Dot()


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add_updater(lambda dt: self.add(make_dot()))
        self.add(square)
        self.play(FadeIn(square), run_time=2)
