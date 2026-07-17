from manim import *


def flag():
    return True


class Branchy(Scene):
    def construct(self):
        a = Square()
        if flag():
            b = a
        else:
            b = Square()
        self.add(a)
        # b may or may not alias a: Maybe aliasing must never fire.
        self.play(Transform(a, b))
