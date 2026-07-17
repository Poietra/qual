from manim import *


def play():
    return None


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(Create(square))
        animations = [FadeIn(square)]
        self.play(*animations)
        play()


class NotAScene:
    def construct(self):
        self.play()
