from manim import Scene as Sc, Square as Sq


class Demo(Sc):
    def construct(self):
        square = Sq()
        self.add(square)
        self.play(square.animate.copy())
