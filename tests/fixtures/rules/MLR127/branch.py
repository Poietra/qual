from manim import *


def flag():
    return True


class Branchy(Scene):
    def construct(self):
        if flag():
            eq = MathTex("a")
        else:
            eq = MathTex("b")
        # The binding is branch-dependent (two assignments): silence, even
        # though the key occurs in neither literal.
        eq.get_part_by_tex("c")
