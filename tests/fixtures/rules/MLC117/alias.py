from manim import LEFT, RIGHT, Scene as Sc, Square as Sq


class Demo(Sc):
    def construct(self):
        square = Sq()
        self.add(square)
        anim = square.animate.shift(RIGHT)
        square.shift(LEFT)
        self.play(anim)
