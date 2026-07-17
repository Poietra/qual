from manim import *


class Present(Scene):
    def construct(self):
        old = Square()
        self.add(old)
        self.replace(old, Circle())


class InsideParent(Scene):
    def construct(self):
        inner = Square()
        group = VGroup(inner)
        self.add(group)
        self.replace(inner, Circle())
