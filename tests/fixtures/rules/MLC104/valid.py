from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        pause = 0
        self.play(Create(square), run_time=2)
        self.wait(0.5)
        self.wait(pause)
        self.play(Wait(run_time=1))
