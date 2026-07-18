from manim.mobject.graphing.functions import ParametricFunction

from manim import Scene


def get_end():
    return 50


class Demo(Scene):
    def construct(self):
        end = get_end()
        curve = ParametricFunction(lambda t: (t, t, 0), t_range=(0, end, 0.0001))
        self.add(curve)
