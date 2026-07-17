from manim import *


def load_key():
    return "x"


class Good(Scene):
    def construct(self):
        eq = MathTex("a^2", "+", "b^2")
        # Exact part: always matches.
        eq.set_color_by_tex("a^2", RED)
        # A substring of a part could be isolated into its own part, so
        # it is not provably dead.
        eq.set_color_by_tex("2", GREEN)
        # Non-literal key: silence.
        eq.get_part_by_tex(load_key())
        # Rebound name: the receiver's literal is no longer provable.
        other = MathTex("x")
        other = MathTex("y")
        other.get_part_by_tex("x")
        # Non-literal constructor argument: silence.
        dynamic = MathTex(load_key())
        dynamic.get_part_by_tex("z")
        self.add(eq)
