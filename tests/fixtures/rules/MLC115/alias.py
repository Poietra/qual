from manim import Dot as Point, Scene as Sc, Square as Sq, VGroup as Bundle


class Demo(Sc):
    def construct(self):
        square = Sq()
        dot = Point()
        group = Bundle(square, dot)
        self.add(group)
        self.remove(dot)
        self.add(group)
