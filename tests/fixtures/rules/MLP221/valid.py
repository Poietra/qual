from manim.mobject.graphing.functions import ParametricFunction

from manim import Axes, Scene


class Demo(Scene):
    def construct(self):
        curve = ParametricFunction(lambda t: (t, t, 0), t_range=(0, 6.28, 0.01))
        wide = ParametricFunction(lambda t: (t, t, 0), t_range=(0, 50))
        axes = Axes()
        graph = axes.plot(lambda x: x * x, x_range=[-5, 5, 0.01])
        self.add(axes)
