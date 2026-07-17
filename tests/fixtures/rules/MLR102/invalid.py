from manim import *


class Bad(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        self.add(square, dot)
        self.play(square.animate)
        self.play(dot.animate, FadeIn(square))
