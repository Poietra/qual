from manim import *


def build():
    return Square()


class Demo(Scene):
    def construct(self):
        box = always_redraw(build)
        self.add(box)
        self.play(FadeIn(box), run_time=2)
