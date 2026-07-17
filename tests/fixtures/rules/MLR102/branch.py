from manim import *


def flag():
    return True


class Branchy(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        if flag():
            # Played only on one path: the fact is Maybe, so the rule
            # stays silent instead of guessing.
            self.play(square.animate)
