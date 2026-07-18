from manim import *


def pick(flag):
    return None


class Demo(Scene):
    def construct(self):
        label = pick(True)
        target = Circle()
        self.add(target)
        self.play(Transform(label, target))
