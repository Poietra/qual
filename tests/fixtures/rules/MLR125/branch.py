from manim import *


def flag():
    return True


class Branchy(Scene):
    def construct(self):
        maybe = Mobject()
        if flag():
            # Branch-dependent add: a Maybe fact must stay silent.
            self.add(maybe)
        self.wait()
