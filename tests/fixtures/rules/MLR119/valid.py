from manim import *


class StaticShot(Scene):
    def construct(self):
        self.play(Create(Square()))
        self.wait()
