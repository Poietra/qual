from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        group = VGroup(square, dot)
        self.add(group)
        self.remove(dot)
        if self.flag:
            self.add(group)
