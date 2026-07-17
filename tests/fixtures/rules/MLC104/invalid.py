from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(Create(square), run_time=0)
        self.play(Create(square), run_time=-2)
        self.wait(0)
        self.wait(-1.5)
        self.wait(duration=0.0)
        self.play(Wait(run_time=0))
