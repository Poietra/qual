from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda dt: dt)  # qual: ignore[MLC105]
