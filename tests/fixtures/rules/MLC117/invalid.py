from manim import *


class Stale(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        anim = square.animate.shift(RIGHT)
        square.shift(LEFT)
        self.play(anim)


class Overwritten(Scene):
    def construct(self):
        circle = Circle()
        self.add(circle)
        first = circle.animate.shift(RIGHT)
        second = circle.animate.rotate(PI)
        self.play(first)
