from manim import *


class Structural(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        group = VGroup(square, dot)
        self.add(group)
        self.remove(dot)
        group.remove(dot)
        self.add(group)


class WholeGroup(Scene):
    def construct(self):
        circle = Circle()
        panel = VGroup(circle)
        self.add(panel)
        self.remove(panel)
        self.add(panel)
