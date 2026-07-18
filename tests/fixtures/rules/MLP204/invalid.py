from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Circle()
        square = Square()
        self.add_updater(lambda dt: self.add(Dot()))
        square.add_updater(lambda m: self.add(Square()))
        self.add(anchor, square)
        self.play(FadeIn(anchor), run_time=2)
