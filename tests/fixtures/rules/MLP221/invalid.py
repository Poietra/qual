from manim.mobject.graphing.functions import ParametricFunction

from manim import Axes, Scene


class Demo(Scene):
    def construct(self):
        curve = ParametricFunction(lambda t: (t, t, 0), t_range=(0, 50, 0.0001))
        axes = Axes()
        graph = axes.plot(lambda x: x * x, x_range=[-10, 10, 0.00005])
        self.add(axes)
