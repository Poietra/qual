from manim import PI, RIGHT, Scene as Sc, Square as Sq


class Demo(Sc):
    def construct(self):
        square = Sq()
        self.add(square)
        self.play(square.animate.shift(RIGHT), square.animate.rotate(PI))
