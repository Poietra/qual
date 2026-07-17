from manim import Dot as Point, Scene as Sc


class Demo(Sc):
    def construct(self):
        dot = Point()
        dot.add(dot)
