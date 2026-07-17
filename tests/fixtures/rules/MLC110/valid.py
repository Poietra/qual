from manim import *


class Demo(Scene):
    def construct(self):
        first = VGroup()
        second = VGroup()
        dot = Dot()
        first.add(dot)
        second.add(dot)
        left = VGroup()
        right = VGroup()
        left.add(right)
        left.remove(right)
        right.add(left)
