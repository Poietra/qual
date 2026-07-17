from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        group = VGroup(square, dot, square)
        band = Group(dot, dot)
        group.add(dot, dot)
