from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        if self.flag:
            self.add(sq)
        self.replace(sq, Circle())
