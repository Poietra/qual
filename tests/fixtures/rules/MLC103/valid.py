from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(Create(square))
        self.play(square.animate.shift(RIGHT))
        factory = make_builder()
        self.play(factory.build, RIGHT)
