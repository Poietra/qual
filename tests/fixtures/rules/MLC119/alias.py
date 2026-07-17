from manim import Circle as Ring, Scene as Sc, Square as Sq


class Demo(Sc):
    def construct(self):
        old = Sq()
        self.replace(old, Ring())
