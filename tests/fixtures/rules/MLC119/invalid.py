from manim import *


class Demo(Scene):
    def construct(self):
        old = Square()
        new = Circle()
        self.replace(old, new)
        self.replace(Square(), new)
