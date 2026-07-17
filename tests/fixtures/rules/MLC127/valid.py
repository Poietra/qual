from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        group = VGroup(square, dot)
        group.add(square.copy(), square.copy())
        items = [square, square]
        cluster = VGroup(*items)
