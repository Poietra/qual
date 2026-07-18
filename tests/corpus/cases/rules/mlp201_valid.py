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


class Suspended(Scene):
    def construct(self):
        # Animation.begin() suspends the animated mobject's updaters
        # (animation.py begin -> suspend_updating), and a one-argument
        # updater does not dynamicize the default wait: the callback
        # provably never runs per frame, so MLP201 stays silent.
        square = Square()
        square.add_updater(lambda m: m.become(Text("hot")))
        self.add(square)
        self.play(FadeIn(square), run_time=2)
        self.wait(3)
