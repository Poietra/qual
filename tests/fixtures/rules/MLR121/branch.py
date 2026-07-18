from manim import *

OUT = [0, 0, 0.5]


class ReboundScene(Scene):
    def construct(self):
        chip = Square()
        self.add(chip)
        # OUT is re-bound in this file: no longer the manim constant.
        chip.shift(OUT)
        self.wait()
