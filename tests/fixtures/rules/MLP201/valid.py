from manim import *


def build_label():
    return MathTex(r"\beta")


class Demo(Scene):
    def construct(self):
        title = MathTex(r"\alpha")
        value = DecimalNumber(0)
        value.add_updater(lambda m: m.set_value(1))
        self.add(title, value, build_label())
        self.play(FadeIn(title), run_time=2)
