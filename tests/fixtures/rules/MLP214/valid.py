from manim import *


class OnlyThreeDistinct(Scene):
    """Three distinct compile keys stay below the DESIGN 7.3 threshold."""

    def construct(self):
        first = MathTex(r"x^2")
        second = MathTex(r"y^2")
        third = MathTex(r"z^2")
        self.play(Write(first), Write(second), Write(third))


class DuplicatesAreOneJob(Scene):
    """Eight constructions, two distinct formulas: two compile jobs."""

    def construct(self):
        rows = VGroup()
        for _ in range(4):
            rows.add(MathTex(r"x^2"))
            rows.add(MathTex(r"y^2"))
        self.play(Write(rows))


class AfterFirstPlay(Scene):
    """Keys built after the first play are not serial cold pre-play jobs."""

    def construct(self):
        title = MathTex(r"t")
        self.play(Write(title))
        first = MathTex(r"a")
        second = MathTex(r"b")
        third = MathTex(r"c")
        fourth = MathTex(r"d")
        self.play(Write(first), Write(second), Write(third), Write(fourth))


class DynamicKeysAreNotCountable(Scene):
    """f-string / dynamic keys are never literal-provable compile keys."""

    def construct(self):
        value = 3
        first = MathTex(f"a = {value}")
        second = MathTex(f"b = {value}")
        third = MathTex(f"c = {value}")
        fourth = MathTex(f"d = {value}")
        self.play(Write(first), Write(second), Write(third), Write(fourth))


class UnmodeledKeyKeywords(Scene):
    """A tex_template / isolation keyword leaves the compile key unproven."""

    def construct(self):
        template = TexTemplate()
        first = MathTex(r"a", tex_template=template)
        second = MathTex(r"b", tex_template=template)
        third = MathTex(r"c", tex_template=template)
        fourth = MathTex(r"d", tex_template=template)
        self.play(Write(first), Write(second), Write(third), Write(fourth))
