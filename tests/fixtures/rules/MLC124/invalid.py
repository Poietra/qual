from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(square.animate.copy())
        dot = Dot()
        self.add(dot)
        self.play(dot.animate.generate_target())
