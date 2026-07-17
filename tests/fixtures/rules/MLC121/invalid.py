from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(lambda m: self.wait(1))
        self.add_updater(lambda dt: self.play(FadeOut(square)))
