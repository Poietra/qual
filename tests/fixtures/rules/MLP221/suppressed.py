from manim.mobject.graphing.functions import ParametricFunction

from manim import Scene


class Demo(Scene):
    def construct(self):
        curve = ParametricFunction(lambda t: (t, t, 0), t_range=(0, 50, 0.0001))  # manim-lint: ignore[MLP221]
        self.add(curve)
