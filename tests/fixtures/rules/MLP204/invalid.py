from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add_updater(lambda dt: self.add(Dot()))
        square.add_updater(lambda m: self.add(Square()))
        self.add(square)
        self.play(FadeIn(square), run_time=2)
